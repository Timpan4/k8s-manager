import { $, browser, expect } from "@wdio/globals";
import { beforeEach, describe, it } from "mocha";

describe("browser mock inspection", () => {
	beforeEach(async () => {
		await browser.url("/");
		await browser.execute(() => localStorage.clear());
		await browser.refresh();
		await $("#workspace-name").setValue("Fast Mock Lab");
		const create = await $("button=Create workspace");
		await create.waitForEnabled();
		await create.click();
	});

	it("shows deterministic mock context and resources", async () => {
		await expect(browser).toHaveTitle(expect.stringContaining("KubeCove"));
		await expect($("body")).toHaveText(expect.stringContaining("mock-dev"));
		await $("button=Resources").click();
		await expect($("body")).toHaveText(expect.stringContaining("payments-api"));
	});

	it("lets page scrolling bypass the resource graph", async () => {
		const originalSize = await browser.getWindowSize();
		await browser.sendCommandAndGetResult("Emulation.setEmulatedMedia", {
			features: [{ name: "prefers-reduced-motion", value: "reduce" }],
		});

		try {
			expect(
				await browser.execute(() =>
					window.matchMedia("(prefers-reduced-motion: reduce)").matches,
				),
			).toBe(true);
			await $("button=Resources").click();
			const graph = await $(".svelte-flow");
			await graph.waitForDisplayed();

			for (const [width, height] of [
				[1100, 600],
				[1440, 600],
			] as const) {
				await browser.setWindowSize(width, height);
				const pageScroller = await $(".overflow-auto.h-full:not(.scrollbar-classic)");
				await browser.execute((element: HTMLElement) => {
					element.scrollTop = 0;
				}, pageScroller);
				await browser
					.action("wheel")
					.scroll({ origin: graph, deltaX: 0, deltaY: 300, duration: 0 })
					.perform();
				await browser.waitUntil(
					async () =>
						(await browser.execute(
							(element: HTMLElement) => element.scrollTop,
							pageScroller,
						)) > 0,
				);
			}

			await $("button=skip graph").click();
			expect(
				await browser.execute(() => document.activeElement?.getAttribute("tabindex")),
			).toBe("-1");
		} finally {
			await browser.setWindowSize(originalSize.width, originalSize.height);
			await browser.sendCommandAndGetResult("Emulation.setEmulatedMedia", { features: [] });
		}
	});

	it("keeps RBAC summary actions visible at compact and wide sizes", async () => {
		const originalSize = await browser.getWindowSize();

		try {
			await $('button[aria-label="Open workspace navigation"]').click();
			const navigation = await $('[data-slot="sheet-content"]');
			await navigation.$('[role="treeitem"]*=RBAC').click();
			const refresh = await $("button=Refresh");
			const investigate = await $("button=Investigate identity");

			for (const [width, height] of [
				[1026, 752],
				[1440, 900],
			] as const) {
				await browser.sendCommandAndGetResult("Emulation.setDeviceMetricsOverride", {
					width,
					height,
					deviceScaleFactor: 1,
					mobile: false,
				});
				await expect(refresh).toBeDisplayed();
				await expect(investigate).toBeDisplayed();

				const layout = await browser.execute(
					(refreshButton: HTMLElement, investigateButton: HTMLElement) => {
						const actions = investigateButton.parentElement;
						const summary = actions?.parentElement;
						const summaryBounds = summary?.getBoundingClientRect();
						const viewport = {
							width: document.documentElement.clientWidth,
							height: document.documentElement.clientHeight,
						};
						const insideViewport = (element: HTMLElement) => {
							const bounds = element.getBoundingClientRect();
							return bounds.left >= 0 && bounds.right <= viewport.width && bounds.top >= 0 && bounds.bottom <= viewport.height;
						};
						const metricCells = ["Identities", "Roles", "Bindings", "High", "Unknown"].map((label) =>
							Array.from(summary?.querySelectorAll("p") ?? []).find((element) => element.textContent?.trim() === label)?.parentElement,
						);
						const investigateBounds = investigateButton.getBoundingClientRect();
						return {
							metricsVisible: metricCells.every(
								(cell) =>
									cell &&
									insideViewport(cell) &&
									cell.getBoundingClientRect().left >= (summaryBounds?.left ?? 1) &&
									cell.getBoundingClientRect().right <= (summaryBounds?.right ?? 0),
							),
							refreshInsideViewport: insideViewport(refreshButton),
							investigateInsideViewport: insideViewport(investigateButton),
							summaryContainsActions: investigateBounds.right <= (summaryBounds?.right ?? 0),
							summaryHasNoHorizontalOverflow: (summary?.scrollWidth ?? 1) <= (summary?.clientWidth ?? 0),
						};
					},
					refresh,
					investigate,
				);

				expect(layout).toEqual({
					metricsVisible: true,
					refreshInsideViewport: true,
					investigateInsideViewport: true,
					summaryContainsActions: true,
					summaryHasNoHorizontalOverflow: true,
				});

				for (const action of [refresh, investigate]) {
					await browser.keys("Tab");
					const focus = await browser.execute((element: HTMLElement) => {
						element.focus();
						return {
							active: document.activeElement === element,
							focusVisible: element.matches(":focus-visible"),
							boxShadow: getComputedStyle(element).boxShadow,
						};
					}, action);
					expect(focus.active).toBe(true);
					expect(focus.focusVisible).toBe(true);
					expect(focus.boxShadow).not.toBe("none");
				}
			}
		} finally {
			await browser.sendCommandAndGetResult("Emulation.clearDeviceMetricsOverride", {});
			await browser.setWindowSize(originalSize.width, originalSize.height);
		}
	});

	it("adapts shared layouts at compact and wide sizes", async () => {
		const originalSize = await browser.getWindowSize();

		try {
			await browser.setWindowSize(1026, 752);

			const clusterSelect = await $("#cluster-select");
			await clusterSelect.waitForDisplayed();
			const compactScope = await browser.execute((select: HTMLElement) => {
				const label = document.getElementById("cluster-select-label");
				const header = select.closest("header");
				const selectBounds = select.getBoundingClientRect();
				const headerBounds = header?.getBoundingClientRect();
				return {
					fullValue: select.getAttribute("title"),
					insideHeader:
						selectBounds.left >= (headerBounds?.left ?? 1) &&
						selectBounds.right <= (headerBounds?.right ?? 0),
					labelVisuallyHidden: label?.getBoundingClientRect().width === 1,
				};
			}, clusterSelect);
			expect(compactScope).toEqual({
				fullValue: "mock-dev",
				insideHeader: true,
				labelVisuallyHidden: true,
			});

			await $('button[aria-label="Open settings"]').click();
			const settingsNav = await $('nav[aria-label="Settings sections"]');
			await settingsNav.waitForDisplayed();
			const compactSettings = await browser.execute((nav: HTMLElement) => {
				const aside = nav.parentElement;
				const content = aside?.nextElementSibling;
				const asideBounds = aside?.getBoundingClientRect();
				const contentBounds = content?.getBoundingClientRect();
				return {
					navigationAboveContent:
						(asideBounds?.bottom ?? 1) <= (contentBounds?.top ?? 0),
				};
			}, settingsNav);
			expect(compactSettings.navigationAboveContent).toBe(true);

			await browser.setWindowSize(1440, 900);
			const wideSettings = await browser.execute((nav: HTMLElement) => {
				const aside = nav.parentElement;
				const layout = aside?.parentElement;
				const content = aside?.nextElementSibling;
				const asideBounds = aside?.getBoundingClientRect();
				const layoutBounds = layout?.getBoundingClientRect();
				const contentBounds = content?.getBoundingClientRect();
				return {
					contentWidth: contentBounds?.width ?? Number.POSITIVE_INFINITY,
					layoutWidth: layoutBounds?.width ?? 0,
					navigationBesideContent:
						(asideBounds?.right ?? 1) <= (contentBounds?.left ?? 0),
				};
			}, settingsNav);
			expect(wideSettings.navigationBesideContent).toBe(true);
			expect(wideSettings.layoutWidth).toBeGreaterThan(1024);
			expect(wideSettings.contentWidth).toBeLessThanOrEqual(1024);

			await $("button=Back to app").click();
			await browser.setWindowSize(1026, 752);
			await $('button[aria-label="Open workspace navigation"]').click();
			const navigation = await $('[data-slot="sheet-content"]');
			await navigation.$('[role="treeitem"]*=GitOps').click();
			await $("button=List").click();
			const details = await $('button[aria-label="Open details for Application platform-argocd in argocd"]');
			await details.waitForDisplayed();
			const compactActions = await browser.execute((button: HTMLElement) => {
				const scroller = button.closest(".overflow-x-auto");
				const scrollerBounds = scroller?.getBoundingClientRect();
				const actions = Array.from(button.parentElement?.querySelectorAll("button") ?? []);
				return {
					insideScroller: actions.every((action) => {
						const bounds = action.getBoundingClientRect();
						return (
							bounds.left >= (scrollerBounds?.left ?? 1) &&
							bounds.right <= (scrollerBounds?.right ?? 0)
						);
					}),
					scrollLeft: scroller?.scrollLeft ?? -1,
				};
			}, details);
			expect(compactActions).toEqual({ insideScroller: true, scrollLeft: 0 });

			await browser.execute((button: HTMLElement) => {
				const scroller = button.closest(".overflow-x-auto");
				if (scroller) scroller.scrollLeft = 200;
			}, details);
			await details.moveTo();
			const scrolledActions = await browser.execute((button: HTMLElement) => {
				const scroller = button.closest(".overflow-x-auto");
				const scrollerBounds = scroller?.getBoundingClientRect();
				const actionArea = button.parentElement;
				const actions = Array.from(actionArea?.querySelectorAll("button") ?? []);
				const canvas = document.createElement("canvas");
				canvas.width = 1;
				canvas.height = 1;
				const context = canvas.getContext("2d");
				if (context && actionArea) {
					context.fillStyle = getComputedStyle(actionArea).backgroundColor;
					context.fillRect(0, 0, 1, 1);
				}
				return {
					insideScroller: actions.every((action) => {
						const bounds = action.getBoundingClientRect();
						return (
							bounds.left >= (scrollerBounds?.left ?? 1) &&
							bounds.right <= (scrollerBounds?.right ?? 0)
						);
					}),
					opaqueHover: context?.getImageData(0, 0, 1, 1).data[3] === 255,
					scrollLeft: scroller?.scrollLeft ?? 0,
				};
			}, details);
			expect(scrolledActions.insideScroller).toBe(true);
			expect(scrolledActions.opaqueHover).toBe(true);
			expect(scrolledActions.scrollLeft).toBeGreaterThan(0);
		} finally {
			await browser.setWindowSize(originalSize.width, originalSize.height);
		}
	});

	it("opens adjacent Argo Application details by accessible name", async () => {
		await $('button[aria-label="Open workspace navigation"]').click();
		const navigation = await $('[data-slot="sheet-content"]');
		await navigation.$('[role="treeitem"]*=GitOps').click();

		const firstDetails = await $('button[aria-label="Open details for Application platform-argocd in argocd"]');
		await firstDetails.click();
		await expect($('button[aria-label="Pin Application platform-argocd"]')).toBeDisplayed();
		await $('button[aria-label="Close resource details"]').click();

		const keyboardDetails = await $('button[aria-label="Open details for Application platform-argocd in argocd"]');
		await keyboardDetails.scrollIntoView();
		await browser.execute((element: HTMLElement) => element.focus(), keyboardDetails);
		await browser.keys("Enter");
		await expect($('button[aria-label="Pin Application platform-argocd"]')).toBeDisplayed();
		await $('button[aria-label="Close resource details"]').click();

		const secondDetails = await $('button[aria-label="Open details for Application platform-cilium in argocd"]');
		await secondDetails.click();
		await expect($('button[aria-label="Pin Application platform-cilium"]')).toBeDisplayed();
		await $('button[aria-label="Close resource details"]').click();
	});

	it("identifies the typed browser-only source", async () => {
		await $('button[aria-label="Open settings"]').click();
		await $("button=Kubeconfig").click();
		await expect($("body")).toHaveText(expect.stringContaining("Browser mock"));
	});
});
